// /get.sh is an alias for /get. Users copy `curl -fsSL https://xerj.org/get.sh | sh`
// as often as the extensionless form; without this route it fell through to the
// static handler and served index.html, so the pipe to `sh` got HTML and any
// sha256 check failed. Re-export the real installer handler.
export { onRequestGet, onRequestHead } from "./get.js";
