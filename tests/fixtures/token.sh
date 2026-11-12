#!/bin/sh
# A stand-in for an auth CLI, such as a Python script that prints a token.
# Usage: sh token.sh <mode> [argument]

case "$1" in
  ok)
    # Prints a token with whitespace around it, as CLIs that end their output with a newline do.
    echo "  t0ken-from-command  "
    ;;
  echo)
    # Prints its argument exactly as received.
    printf '%s\n' "$2"
    ;;
  fail)
    # Fails after writing to both streams.
    echo "sekrit-stdout-of-failed-command"
    echo "not logged in: run acme login" >&2
    exit 3
    ;;
  empty)
    # Succeeds but prints nothing.
    exit 0
    ;;
  *)
    echo "token.sh: unknown mode '$1'" >&2
    exit 64
    ;;
esac
