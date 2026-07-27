def handler(request):
    uid = request.args.get("id")     # source
    return do_query(uid)             # -> sink in another function
def do_query(v):
    cur.execute("SELECT * FROM u WHERE id=" + v)   # sql sink
def safe(request):
    v = html.escape(request.args.get("x"))         # sanitized -> not a finding
    cur.execute("SELECT " + v)
