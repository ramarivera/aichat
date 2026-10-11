module completions {

  def "nu-complete rr-aichat completions" [] {
    [ "bash" "zsh" "fish" "powershell" "nushell" ]
  }

  def "nu-complete rr-aichat model" [] {
    ^rr-aichat --list-models
    | lines
    | parse "{value}"
  }

  def "nu-complete rr-aichat role" [] {
    ^rr-aichat --list-roles
    | lines
    | parse "{value}"
  }

  def "nu-complete rr-aichat session" [] {
    ^rr-aichat --list-sessions
    | lines
    | parse "{value}"
  }

  def "nu-complete rr-aichat agent" [] {
    ^rr-aichat --list-agents
    | lines
    | parse "{value}"
  }

  def "nu-complete rr-aichat rag" [] {
    ^rr-aichat --list-rags
    | lines
    | parse "{value}"
  }

  def "nu-complete rr-aichat macro" [] {
    ^rr-aichat --list-macros
    | lines
    | parse "{value}"
  }

  export extern rr-aichat [
    --model(-m): string@"nu-complete rr-aichat model"   # Select a LLM model
    --prompt                                            # Use the system prompt
    --role(-r): string@"nu-complete rr-aichat role"     # Select a role
    --session(-s): string@"nu-complete rr-aichat session" # Start or join a session
    --empty-session                                     # Ensure the session is empty
    --save-session                                      # Ensure the new conversation is saved to the session
    --agent(-a): string@"nu-complete rr-aichat agent"   # Start a agent
    --agent-variable                                    # Set agent variables
    --rag: string@"nu-complete rr-aichat rag"           # Start a RAG
    --rebuild-rag                                       # Rebuild the RAG to sync document changes
    --macro: string@"nu-complete rr-aichat macro"       # Execute a macro
    --serve                                             # Serve the LLM API and WebAPP
    --execute(-e)                                       # Execute commands in natural language
    --code(-c)                                          # Output code only
    --file(-f): string                                  # Include files, directories, or URLs
    --no-stream(-S)                                     # Turn off stream mode
    --dry-run                                           # Display the message without sending it
    --info                                              # Display information
    --sync-models                                       # Sync models updates
    --list-models                                       # List all available chat models
    --list-roles                                        # List all roles
    --list-sessions                                     # List all sessions
    --list-agents                                       # List all agents
    --list-rags                                         # List all RAGs
    --list-macros                                       # List all macros
    ...text: string                                     # Input text
    --help(-h)                                          # Print help
    --version(-V)                                       # Print version
  ]

}

export use completions *
