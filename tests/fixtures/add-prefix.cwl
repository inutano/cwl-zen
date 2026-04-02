cwlVersion: v1.2
class: CommandLineTool
baseCommand: []
requirements:
  - class: ShellCommandRequirement
inputs:
  prefix:
    type: string
  input_file:
    type: File
arguments:
  - shellQuote: false
    valueFrom: |
      echo "$(inputs.prefix)_\$(basename $(inputs.input_file.path))"
stdout: output.txt
outputs:
  output:
    type: File
    outputBinding:
      glob: output.txt
