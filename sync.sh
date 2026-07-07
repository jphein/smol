#!/usr/bin/env bash
cd /home/jp/Projects/smol && git add -A && git commit -m "sync: ${*:-update}" && git push
