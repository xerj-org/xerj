def query_orders(session):
    """Old broker demo: query live orders via the demo broker."""
    orders = session.query_orders()
    for order in orders:
        print("live order", order)
    return orders

def live_orders_demo():
    return query_orders(None)
