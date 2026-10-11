#!/bin/sh
case " $* " in
  *" ls "*)
    printf '%s\n' "11111111-1111-1111-1111-111111111111 30000000-0000-0000-0000-000000000001 /library/a/01.flac"
    ;;
  *)
    eval "p=\${$#}"
    mkfifo "$p.control"
    printf 'r' > "$p.ready"
    read x < "$p.control"
    printf 'partial-without-newline'
    printf 'p' > "$p.partial"
    read x < "$p"
    ;;
esac
