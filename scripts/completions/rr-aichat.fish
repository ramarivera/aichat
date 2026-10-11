complete -c rr-aichat -s m -l model -x -a "(rr-aichat --list-models)" -d 'Select a LLM model' -r
complete -c rr-aichat -l prompt -d 'Use the system prompt'
complete -c rr-aichat -s r -l role -x -a "(rr-aichat --list-roles)" -d 'Select a role' -r
complete -c rr-aichat -s s -l session -x  -a "(rr-aichat --list-sessions)" -d 'Start or join a session' -r
complete -c rr-aichat -l empty-session -d 'Ensure the session is empty'
complete -c rr-aichat -l save-session -d 'Ensure the new conversation is saved to the session'
complete -c rr-aichat -s a -l agent -x  -a "(rr-aichat --list-agents)" -d 'Start a agent' -r
complete -c rr-aichat -l agent-variable -d 'Set agent variables'
complete -c rr-aichat -l rag -x  -a"(rr-aichat --list-rags)" -d 'Start a RAG' -r
complete -c rr-aichat -l rebuild-rag -d 'Rebuild the RAG to sync document changes'
complete -c rr-aichat -l macro -x  -a"(rr-aichat --list-macros)" -d 'Execute a macro' -r
complete -c rr-aichat -l serve -d 'Serve the LLM API and WebAPP'
complete -c rr-aichat -s e -l execute -d 'Execute commands in natural language'
complete -c rr-aichat -s c -l code -d 'Output code only'
complete -c rr-aichat -s f -l file -d 'Include files, directories, or URLs' -r -F
complete -c rr-aichat -s S -l no-stream -d 'Turn off stream mode'
complete -c rr-aichat -l dry-run -d 'Display the message without sending it'
complete -c rr-aichat -l info -d 'Display information'
complete -c rr-aichat -l sync-models -d 'Sync models updates'
complete -c rr-aichat -l list-models -d 'List all available chat models'
complete -c rr-aichat -l list-roles -d 'List all roles'
complete -c rr-aichat -l list-sessions -d 'List all sessions'
complete -c rr-aichat -l list-agents -d 'List all agents'
complete -c rr-aichat -l list-rags -d 'List all RAGs'
complete -c rr-aichat -l list-macros -d 'List all macros'
complete -c rr-aichat -s h -l help -d 'Print help'
complete -c rr-aichat -s V -l version -d 'Print version'
