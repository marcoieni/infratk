# Infra Tool Kit

Utility for managing Rust [simpleinfra](https://github.com/rust-lang/simpleinfra).

## Features

- Apply the current branch's changes to every affected Terraform/Terragrunt
  root module, logging into each AWS account and preserving the normal apply
  confirmation prompt. Fetches the default branch from `origin` before calculating
  changes, using the updated remote reference as the comparison base. Ignores
  Markdown documentation changes when selecting affected modules. Reinitializes
  modules whose backend configuration changed, without migrating state. If you
  decline a module's apply, asks whether to continue with the remaining modules
- Update Terragrunt states verifying that the changes don't edit the state
- Run `plan` for every lockfile of a PR
- Show the dependency graph of the modules

## Useful aliases

```bash
alias ill='eval "$(infratk legacy-login)"'
alias icd='eval "$(infratk cd)"'
```
