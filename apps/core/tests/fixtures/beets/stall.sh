#!/bin/sh
eval "p=\${$#}"
printf 'r' > "$p.ready"
read x < "$p"
