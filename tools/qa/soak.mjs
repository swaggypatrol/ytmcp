#!/usr/bin/env node
// Optional entry point for recording personal use of the installed app.
process.argv.splice(2, 0, "record");
await import("./daily-recorder.mjs");
