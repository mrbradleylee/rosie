# Prompt customizations for cmd-printer
# Adjust the user prompt wording to better fit the LLM’s response format.
# The program now wraps the user prompt with clear instructions:
# "You are an assistant that outputs the exact shell command for the following task, nothing else:"

# For example, if the user provides: "check for brew updates"
# the LLM will be asked to return just: "brew update && brew upgrade"

# To tweak the instructions, edit the string in `main.rs` above.
