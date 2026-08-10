// /get.ps1 — the Windows installer, same counter as /get.
//
// PRIVACY POSTURE: identical to functions/get.js, which is the only place the
// behaviour is implemented. Read the header comment there — it is the whole
// contract (no IP, no cookie, no identifier, fail-open).
//
// Pages derives the route from the file name minus the .js extension, so this
// file answers /get.ps1. It holds no logic of its own on purpose: the handler
// branches on request pathname, so there is exactly one implementation and
// exactly one set of privacy rules to audit.
export { onRequestGet, onRequestHead } from './get.js';
