class App {
  void handler(HttpServletRequest request) { query(request.getParameter("id")); }
  void query(String id) { stmt.executeQuery("SELECT * FROM t WHERE id=" + id); }
}
