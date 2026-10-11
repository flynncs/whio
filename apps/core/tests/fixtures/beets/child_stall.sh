#!/bin/sh
eval "p=\${$#}"
sh -c 'eval "q=\${$#}"; exec 3> "$q.hold"; read x < "$q"' _ "$p" &
printf 'r' > "$p.ready"
read x < "$p"
