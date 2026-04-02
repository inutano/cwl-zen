cwlVersion: v1.2
class: CommandLineTool
baseCommand: cat
inputs:
  input_file:
    type: File
    inputBinding:
      position: 1
stdout: output.txt
outputs:
  output:
    type: File
    outputBinding:
      glob: output.txt
