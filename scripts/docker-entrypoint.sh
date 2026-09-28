#!/bin/sh
# Container profiles must be exclusive to this container while it is running.
set -eu

: "${HOME:?HOME must be set and nonempty}"
profile_dir="$HOME/.tcode/chrome"

rm -f -- \
    "$profile_dir/SingletonLock" \
    "$profile_dir/SingletonSocket" \
    "$profile_dir/SingletonCookie"

exec tcode "$@"
