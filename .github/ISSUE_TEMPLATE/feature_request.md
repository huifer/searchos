name: Feature request
description: Propose a capability change (must reference an RFC or open an RFC change first)
labels: ["type:feature"]
body:
  - type: textarea
    id: summary
    attributes:
      label: Summary
      description: What capability should change and why?
    validations:
      required: true
  - type: textarea
    id: rfc
    attributes:
      label: RFC reference
      description: Which RFC governs this area? RFC changes require an ADR.
      placeholder: RFC-009 9.4
  - type: textarea
    id: acceptance
    attributes:
      label: Acceptance criteria
      description: Verifiable interface / data structure / acceptance conditions (RFC 0.2 requires them).
    validations:
      required: true
