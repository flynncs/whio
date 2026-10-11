#!/bin/sh
case " $* " in
  *" ls "*)
    printf '%s\n' "11111111-1111-1111-1111-111111111111 30000000-0000-0000-0000-000000000001 /library/a/01.flac"
    ;;
  *)
    eval "p=\${$#}"
    sh -c 'eval "q=\${$#}"; exec 3> "$q.hold"; read x < "$q"' _ "$p" &
    printf 'r' > "$p.ready"
    exit 0
    ;;
esac
