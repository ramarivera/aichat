_rr_aichat_bash() {
    if [[ -n "$READLINE_LINE" ]]; then
        READLINE_LINE=$(rr-aichat -e "$READLINE_LINE")
        READLINE_POINT=${#READLINE_LINE}
    fi
}
bind -x '"\ee": _rr_aichat_bash'