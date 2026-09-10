# Rat Commander: change the shell's directory to wherever `rc` was when it quit.
#
# Source this from ~/.config/fish/config.fish, then use `rcd` instead of `rc`:
#
#     source /usr/share/rat-commander/rc.fish
function rcd --description 'Run Rat Commander and cd to the directory it quit in'
    set -l rc_dir (mktemp (test -n "$TMPDIR"; and echo $TMPDIR; or echo /tmp)/rc-lastdir.XXXXXX)
    or return 1
    rc --print-last-dir $rc_dir $argv
    set -l rc_status $status
    if test -s $rc_dir
        set -l rc_target (cat $rc_dir)
        test -d "$rc_target"; and cd "$rc_target"
    end
    rm -f $rc_dir
    return $rc_status
end
