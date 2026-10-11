def _rr_aichat_nushell [] {
    let _prev = (commandline)
    if ($_prev != "") {
        print '⌛'
        commandline edit -r (rr-aichat -e $_prev)
    }
}

$env.config.keybindings = ($env.config.keybindings | append {
        name: rr_aichat_integration
        modifier: alt
        keycode: char_e
        mode: [emacs, vi_insert]
        event:[
            {
                send: executehostcommand,
                cmd: "_rr_aichat_nushell"
            }
        ]
    }
)