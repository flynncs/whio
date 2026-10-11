#!/bin/sh
case " $* " in
  *" ls "*)
    :
    ;;
  *)
    eval "p=\${$#}"
    printf '%s\n' "$*" > "$p.args"
    ;;
esac
