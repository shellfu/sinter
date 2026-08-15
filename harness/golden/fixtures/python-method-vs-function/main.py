from util import start


class Server:
    # Same name as the imported module-level function.
    def start(self):
        return 1

    def run(self):
        self.start()
        # Bare name: methods are not in scope, this is util.start.
        return start()
