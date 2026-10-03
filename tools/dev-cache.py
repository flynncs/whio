import argparse
import sys
from dev_checks import cache_entries, cache_size, docker_cache_size, prune_caches, runtime_root


def main():
    parser = argparse.ArgumentParser(description='inspect or prune idle tool-owned core build caches')
    parser.add_argument('--prune', action='store_true')
    args = parser.parse_args()
    try:
        root = runtime_root()
        if args.prune:
            import dev
            with dev.state_lock():
                dev.cleanup()
            prune_caches(root, remove_all=True)
        entries = cache_entries(root)
        total = 0
        for path in entries:
            size = cache_size(path)
            total += size
            print(f'{path.name}: {size / 1024 ** 3:.2f} GiB')
        total += docker_cache_size(root)
        print(f'managed caches: {total / 1024 ** 3:.2f} / 16 GiB; active caches are retained')
        return 0
    except (OSError, RuntimeError) as error:
        print(f'cache maintenance failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
