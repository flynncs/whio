#!/bin/sh
eval "p=\${$#}"
printf 'r' > "$p.ready"
exec 3> "$p.hold"
read x < "$p"
