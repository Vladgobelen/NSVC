#!/bin/sh
cd "/home/diver/sources/RUST/NSVoice/rust_web_chat_c/"
j=$(date)
git add .
git commit -m "$1 $j"
git push git@github.com:Vladgobelen/NSVC.git

