import argparse
import collections
import json
import pathlib
import unicodedata


def norm(value):
    return ' '.join(unicodedata.normalize('NFC', value or '').lower().translate(str.maketrans({'’': "'", '‘': "'", '–': '-', '—': '-'})).split())


def audit(directory, output):
    output.mkdir(parents=True, exist_ok=True)
    report = {}
    for dataset in ('frozen', 'homelab'):
        source = directory / dataset
        read = lambda name: [json.loads(line) for line in (source / name).read_text().splitlines() if line]
        rows, library = read('observations.jsonl'), read('library.jsonl')
        recording_ids = {r.get('musicbrainz_recording_id') for r in library if r.get('musicbrainz_recording_id')}
        pairs, conflicts = {}, {}
        reviewed = []
        for row in rows:
            local, label, remote = row.get('target_local'), row.get('oracle'), row['remote']
            agreement = bool(local and label and local.get('musicbrainz_recording_id') and local['musicbrainz_recording_id'] == label.get('recording_id'))
            if local and label:
                classification = 'identifier_agreement' if agreement else 'identifier_conflict_with_target'
            elif local:
                classification = 'unlabelled_with_target'
            elif label:
                classification = 'labelled_miss' if label.get('recording_id') in recording_ids else 'labelled_absent_no_target'
            else:
                classification = 'unlabelled_no_target'
            support = None
            if local:
                ra, la = {norm(a) for a in remote['artists']}, {norm(a) for a in local['artists']}
                rd, ld = remote.get('duration_ms'), local.get('duration_ms')
                support = bool(norm(remote['title']) == norm(local['title']) and la and la <= ra and rd and ld and abs(rd-ld) <= (1000 if la < ra else 2000))
            reason = ('MusicBrainz URL label agrees with the local recording ID; no audio verification' if agreement else
                      'conflicting recording IDs remain unresolved, regardless of title or duration agreement' if local and label else
                      'compatible metadata supports a reversible search substitution, but does not verify audio identity' if support else
                      'no independent evidence establishes recording identity')
            result = dict(dataset=dataset, query=row['query'], position=row['position'], video_id=row['video_id'],
                target=row['decision']['target'], same_recording=True if agreement else 'unsure',
                classification=classification, metadata_support=support, reason=reason,
                present_in_library=True if label and label.get('recording_id') in recording_ids else 'unsure')
            reviewed.append(result)
            if local:
                pairs[(row['video_id'], row['decision']['target'])] = result
            if local and label and not agreement:
                conflicts[(row['video_id'], row['decision']['target'])] = row
        values = list(pairs.values())
        agreed = sum(r['same_recording'] is True for r in values)
        target_count = len(values)
        counts = dict(collections.Counter(r['classification'] for r in reviewed))
        report[dataset] = dict(observations=len(rows), unique_video_target_pairs=target_count,
            independently_audio_verified=0, identifier_agreement_pairs=agreed,
            unresolved_target_pairs=target_count-agreed, classification_counts=counts,
            precision=None, recall=None,
            conditional_precision_interval=[agreed/target_count, 1.0] if target_count else None,
            interval_assumption='identifier agreements are correct; every unresolved target could be wrong or right; not a statistical confidence interval',
            metadata_exact_title_supported_pairs=sum(r['metadata_support'] is True for r in values),
            caveat='automated evidence audit, not independent audio adjudication; repeated observations are not independent samples')
        (output / f'{dataset}-adjudicated.jsonl').write_text(''.join(json.dumps(r, ensure_ascii=False)+'\n' for r in reviewed))
        (output / f'{dataset}-identifier-conflicts.json').write_text(json.dumps(list(conflicts.values()),indent=2,ensure_ascii=False))
    (output / 'summary.json').write_text(json.dumps(report,indent=2))
    print(json.dumps(report,indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('package', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    args = parser.parse_args()
    audit(args.package, args.output)
