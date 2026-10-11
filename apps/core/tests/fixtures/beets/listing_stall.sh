#!/bin/sh
lib=""
prev=""
for a in "$@"; do
  [ "$prev" = "-l" ] && lib="$a"
  prev="$a"
done
dir=$(dirname "$lib")
case " $* " in
  *" ls "*)
    p=$(cat "$dir/import_path")
    printf 'l' > "$p.listing"
    read x < "$dir/listing.fifo"
    ;;
  *)
    eval "p=\${$#}"
    printf '%s\n' "$*" > "$p.args"
    printf '%s\n' "$p" > "$dir/import_path"
    mkfifo "$dir/listing.fifo"
    ;;
esac
