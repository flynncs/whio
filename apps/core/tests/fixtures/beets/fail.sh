#!/bin/sh
eval "p=\${$#}"
printf '%s\n' "$*" > "$p.args"
exit 3
