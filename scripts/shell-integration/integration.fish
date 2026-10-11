function _rr_aichat_fish
    set -l _old (commandline)
    if test -n $_old
        echo -n "⌛"
        commandline -f repaint
        commandline (rr-aichat -e $_old)
    end
end
bind \ee _rr_aichat_fish