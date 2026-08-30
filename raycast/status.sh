#!/bin/bash
# @raycast.schemaVersion 1
# @raycast.title Ghostty Status
# @raycast.mode fullOutput
# @raycast.icon 📊
# @raycast.description Show the live Ghostty configuration and check it
# @raycast.author murat

GHONFIG="$HOME/.cargo/bin/ghonfig"
"$GHONFIG" status
echo
"$GHONFIG" check
