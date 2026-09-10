# Rat Commander: change the shell's directory to wherever `rc` was when it quit.
#
# Source this from ~/.bashrc or ~/.zshrc, then use `rcd` instead of `rc`:
#
#     source /usr/share/rat-commander/rc.sh
#
# `rc` itself is untouched — `rcd` is the wrapper that follows the last panel
# directory, the same way Midnight Commander's `mc -P` wrapper does.
# Works in bash, zsh and any other POSIX shell with `local`.
rcd() {
    local _rc_dir _rc_status _rc_target
    _rc_dir=$(mktemp "${TMPDIR:-/tmp}/rc-lastdir.XXXXXX") || return 1

    rc --print-last-dir "$_rc_dir" "$@"
    # Captured on its own line: `local` would clobber $? if it were combined.
    _rc_status=$?

    if [ -s "$_rc_dir" ]; then
        # Only the trailing newline is stripped, so a directory name may
        # contain spaces (or anything else but a newline).
        _rc_target=$(cat "$_rc_dir")
        if [ -d "$_rc_target" ]; then
            cd -- "$_rc_target" || :
        fi
    fi

    rm -f "$_rc_dir"
    return "$_rc_status"
}
