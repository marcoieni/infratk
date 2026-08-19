# Infra Tool Kit

Utility for managing Rust [simpleinfra](https://github.com/rust-lang/simpleinfra).

## Features

- Apply the current branch's changes to every affected Terraform/Terragrunt
  root module, logging into each AWS account and preserving the normal apply
  confirmation prompt. Existing Datadog AWS integrations are automatically
  imported into the unified resource before deprecated state entries are removed
- Update Terragrunt states verifying that the changes don't edit the state
- Run `plan` for every lockfile of a PR
- Show the dependency graph of the modules

The Datadog AWS state migration uses the same `DD_API_KEY`, `DD_APP_KEY`, and
optional `DD_HOST` environment variables as the Terraform Datadog provider.

## Useful aliases

```bash
alias ill='eval "$(infratk legacy-login)"'
alias icd='eval "$(infratk cd)"'
```
