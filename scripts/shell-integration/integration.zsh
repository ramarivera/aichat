_rr_aichat_zsh() {
    if [[ -n "$BUFFER" ]]; then
        local _old=$BUFFER
        BUFFER+="⌛"
        zle -I && zle redisplay
        BUFFER=$(rr-aichat -e "$_old")
        zle end-of-line
    fi
}
zle -N _rr_aichat_zsh
bindkey '\ee' _rr_aichat_zsh