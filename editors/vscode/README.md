# Jig for Visual Studio Code

Syntax highlighting for `.jig` files, and from the `jig lsp` language server: diagnostics
as you type (every check `jig check` makes, including dead code), the type of an
expression or a function's signature on hover, go to definition, and format document.

The extension runs the `jig` command, which `sh bootstrap/install.sh` installs; set
`jig.path` if it is not on the `PATH`.

## Install

```
cd editors/vscode
npm install
npx @vscode/vsce package
code --install-extension jig-0.1.0.vsix
```

Any editor with a language client can use `jig lsp` the same way: it speaks the Language
Server Protocol over standard input and output.
