#!/usr/bin/env bash
set -euo pipefail

# This is effectively just a giant smoke test of the whole system.

# Use 10 connections by default, to verify it can actually do things in parallel. This is locally
# configurable, but I do want a consistent value for CI that actually tests for what I need.
# Note: the CI environment only offers 2 logical cores. This is worked around by simply using a
# high nice value.
CONCURRENCY=10

# Use a connection timeout of 1 second by default, to align with the load tester's own default
# server no-data timeout.
CONN_TIMEOUT=1.0

# Limit the test duration to 60 seconds by default, but leave it configurable for local testing.
TEST_DURATION=60

BIN=./target/debug/loadtest
PORT=

while getopts ':rl:p:c:t:' opt; do
    case "$opt" in
        r)
            BIN=./target/release/loadtest
            ;;
        p)
            if [[ -z "$OPTARG" ]]; then
                echo 'Port number must not be empty.' >&2
                exit 1
            fi
            PORT="$OPTARG"
            ;;
        c)
            if [[ -z "$OPTARG" ]]; then
                echo 'Concurrency must not be empty.' >&2
                exit 1
            fi
            CONCURRENCY="$OPTARG"
            ;;
        t)
            if [[ -z "$OPTARG" ]]; then
                echo 'Test duration must not be empty.' >&2
                exit 1
            fi
            TEST_DURATION="$OPTARG"
            ;;
        *)
            echo "Unknown option '$OPTARG'." >&2
            exit 1
            ;;
    esac
done

if [[ -z "$PORT" ]]; then
    echo 'A port is required.'
fi

# This bit is designed to allow for repeated and even potentially nested calls.
server_pid=0
client_pid=0
timeout_pid=0

cleanup() {
    [[ $server_pid -ne 0 ]] && kill $server_pid; server_pid=0
    [[ $client_pid -ne 0 ]] && kill $client_pid; client_pid=0
    [[ $timeout_pid -ne 0 ]] && kill $timeout_pid; timeout_pid=0
}

trap 'exit 1' TERM INT QUIT ALRM USR1 HUP
trap cleanup EXIT

# Run with a higher nice value so it can't interfere with this script.
nice -n 10 "${BIN}" server "tcp://localhost:$PORT" --connection-timeout "$CONN_TIMEOUT" &
server_pid=$!

# Give it time to start up. (It's normally near instant, so it shouldn't take long.)
sleep 1

# Run with a higher nice value so it can't interfere with this script.
nice -n 10 "${BIN}" client "tcp://localhost:$PORT" --concurrency "$CONCURRENCY" &
client_pid=$!

sleep "$TEST_DURATION" &
timeout_pid=$!

exit_code=1
waited_pid='<unset>'
wait -fp waited_pid -n "$timeout_pid" "$client_pid" "$server_pid" || exit_code=$?

case "$waited_pid" in
    "$timeout_pid")
        timeout_pid=0
        exit_code=0
        ;;
    "$client_pid")
        client_pid=0
        ;;
    "$server_pid")
        server_pid=0
        ;;
    *)
        echo "Expected one of the following PIDs, but found $waited_pid:" >&2
        echo "- Client PID: $client_pid" >&2
        echo "- Server PID: $server_pid" >&2
        echo "- Timeout PID: $timeout_pid" >&2
esac

exit "$exit_code"
