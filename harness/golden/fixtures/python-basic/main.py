import os.path

from util import helper


# Greets the given name.
def greet(name):
    return "hello " + name


class Server:
    # Starts the server.
    def start(self):
        print(greet(helper("world")))
