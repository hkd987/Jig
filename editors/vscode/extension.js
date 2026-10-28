// Starts `jig lsp` for .jig files: diagnostics as you type, hovers, go to definition and
// formatting. The command is the `jig.path` setting (`jig` on the PATH by default).

const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let client;

function activate() {
  const command = vscode.workspace.getConfiguration("jig").get("path") || "jig";
  client = new LanguageClient(
    "jig",
    "Jig",
    { command, args: ["lsp"] },
    { documentSelector: [{ scheme: "file", language: "jig" }] },
  );
  return client.start();
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
