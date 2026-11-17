#!/bin/sh
# POSIX timing notes for the nextest archive pilot. Does nothing unless executed.

notes_path=$1
if [ -z "$notes_path" ]; then
    echo "usage: $0 <notes-path>" >&2
    exit 1
fi

case $0 in
/*) script_dir=$(dirname "$0") ;;
*) script_dir=$(cd "$(dirname "$0")" && pwd) ;;
esac

cd "$script_dir" || exit 1

if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    target_dir=$CARGO_TARGET_DIR
else
    target_dir="$script_dir/target"
fi

archive_file="$target_dir/pilot.tar.zst"

notes_dir=$(dirname "$notes_path")
if [ ! -d "$notes_dir" ]; then
    mkdir -p "$notes_dir" || exit 1
fi

: >"$notes_path"

run_timed() {
    start=$(date +%s)
    if "$@" >/dev/null 2>&1; then
        end=$(date +%s)
        echo $((end - start))
        return 0
    fi
    echo "failed"
    return 1
}

export CARGO_TARGET_DIR="$target_dir"

if doc_result=$(run_timed cargo test --doc); then
    :
else
    doc_result=failed
fi
echo "doctest_seconds=$doc_result" >>"$notes_path"

if lib_result=$(run_timed cargo test --lib); then
    :
else
    lib_result=failed
fi
echo "lib_test_seconds=$lib_result" >>"$notes_path"

have_nextest=false
if command -v cargo >/dev/null 2>&1; then
    if cargo nextest --version >/dev/null 2>&1; then
        have_nextest=true
    fi
fi

if [ "$have_nextest" = false ]; then
    echo "nextest=absent" >>"$notes_path"
    echo "archive_create_seconds=skipped" >>"$notes_path"
    echo "filtered_run_seconds=skipped" >>"$notes_path"
    echo "archive_bytes=unknown" >>"$notes_path"
    exit 0
fi

echo "nextest=present" >>"$notes_path"

archive_ok=false
if cargo nextest archive --help >/dev/null 2>&1; then
    if archive_result=$(run_timed cargo nextest archive --archive-file "$archive_file"); then
        archive_ok=true
        echo "archive_create_seconds=$archive_result" >>"$notes_path"
    else
        echo "archive_create_seconds=failed" >>"$notes_path"
    fi
else
    echo "archive_create_seconds=unsupported" >>"$notes_path"
fi

if [ -f "$archive_file" ]; then
    archive_bytes=$(wc -c <"$archive_file" | tr -d '[:space:]')
    echo "archive_bytes=$archive_bytes" >>"$notes_path"
else
    echo "archive_bytes=unknown" >>"$notes_path"
fi

if [ "$archive_ok" = true ]; then
    if filtered_result=$(run_timed cargo nextest run --archive-file "$archive_file" -E 'test(one_plus_one_is_two)'); then
        echo "filtered_run_seconds=$filtered_result" >>"$notes_path"
    else
        echo "filtered_run_seconds=failed" >>"$notes_path"
    fi
else
    echo "filtered_run_seconds=skipped" >>"$notes_path"
fi

exit 0