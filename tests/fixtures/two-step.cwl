cwlVersion: v1.2
class: Workflow
inputs:
  message:
    type: string
  prefix:
    type: string
steps:
  echo_step:
    run: echo.cwl
    in:
      message: message
    out: [output]
  cat_step:
    run: cat.cwl
    in:
      input_file: echo_step/output
    out: [output]
outputs:
  final_output:
    type: File
    outputSource: cat_step/output
