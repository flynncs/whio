#!/bin/sh
case " $* " in
  *" ls "*)
    printf '%s\n' " 30000000-0000-0000-0000-000000000003 /library/single.flac"
    ;;
  *)
    eval "p=\${$#}"
    printf '%s\n' "$*" > "$p.args"
    ;;
esac
