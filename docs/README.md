# Documentation

The [README](../README.md) says what paperjam is, how to install it and how to use it day to day. These pages go deeper. Pick the one that matches what you are doing.

| Page | Read it when |
|---|---|
| [Configuration](configuration.md) | You run the server and need to set or debug a variable, a reverse proxy, the inbox folder or a different LLM endpoint |
| [MCP server](mcp.md) | You connect an agent, call the tools yourself or want the search syntax |
| [iOS Shortcut](ios-shortcut.md) | You want to share photos and files to paperjam from an iPhone, or need the upload request for another tool |
| [Architecture](architecture.md) | You contribute, or want to know what is stored where and what happens to a file after upload |
| [Identifier validators](validators.md) | A document was flagged `checksum_failed`, or you want to add a validator |
| [Releasing](releasing.md) | You cut a release, or want to verify that an image came from this repository |

The REST API describes itself: the server serves its OpenAPI document at `/api/v1/openapi.json`.

Other files in the repository: the [changelog](../CHANGELOG.md), the [contribution guide](../CONTRIBUTING.md) and the [security policy](../SECURITY.md).
