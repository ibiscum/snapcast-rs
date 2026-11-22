# Repository Instructions

## Sensitive Data

Never include these in code, tests, logs, screenshots, commits, or reports:

- Access tokens
- API keys
- Private keys
- Passwords
- Session cookies
- Full customer records
- Payment data
- Production database exports

## High-Risk Changes

Use read-only analysis and request approval before changing:

- Authentication
- Authorization
- Encryption
- Secret storage
- File uploads
- Administrative permissions
- Audit logging
- Data deletion
- Network or sandbox permissions

## Compatibility

- Preserve documented public APIs unless a breaking change is explicitly approved.
- Consider users on supported runtime versions.
- Do not change default behavior silently.
- Include migration guidance for intentional breaking changes.

## Dependencies

- Avoid adding runtime dependencies when the standard library or existing dependencies are sufficient.
- Explain bundle-size and maintenance impact for every new runtime dependency.
- Update lockfiles only when dependency declarations change.

## Tests

- Add regression coverage for bug fixes.
- Add compatibility tests for public API changes.
- Keep examples executable.
- Run the full supported-version matrix when configuration is available.

## Documentation

Update the relevant documentation when changing:

- Public functions
- Configuration
- Default behavior
- Error behavior
- Supported environments

# Testing and Review Instructions

## Test Strategy

- Test public behavior, not private implementation details.
- Add regression tests for confirmed bugs.
- Include expected failure cases.
- Include authorization and validation cases when relevant.
- Use deterministic fixtures.
- Do not rely on test order.
- Do not remove or weaken an existing test without explaining why it is invalid.

## Test Selection

- Run the smallest relevant test set during iteration.
- Run the broader affected suite before reporting completion.
- Run the full suite when changing shared infrastructure.
- Report commands that were skipped or unavailable.

## Code Review

Review changes for:

- Requirement coverage
- Correctness
- Edge cases
- Error handling
- Authentication and authorization
- Data integrity
- Security
- Backward compatibility
- Performance
- Unnecessary complexity
- Duplicate logic
- Missing tests
- Accidental changes

## Review Output

For each issue, report:

- Severity
- File and location
- Evidence
- Impact
- Recommended correction
- Verification method

Do not modify files during the initial review unless the task explicitly requests fixes.