name: Bug report
description: Something in SearchOS misbehaves (RFC violation, wrong decision, crash)
labels: ["type:bug"]
body:
  - type: textarea
    id: what-happened
    attributes:
      label: What happened?
      description: Describe the incorrect behavior, including the exact command or API call.
    validations:
      required: true
  - type: textarea
    id: expected
    attributes:
      label: Expected behavior
      description: Which RFC section defines the expected behavior?
      placeholder: RFC-004 4.4 Rule 1 ...
  - type: input
    id: trace
    attributes:
      label: Trace id (if applicable)
      placeholder: trace_...
  - type: textarea
    id: repro
    attributes:
      label: Reproduction steps
      placeholder: |
        1. searchos init
        2. searchos search "..." --mode local-only
  - type: input
    id: version
    attributes:
      label: Version
      placeholder: git commit or searchos version
