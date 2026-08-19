from animal import Animal


class Dog(Animal):
    """A dog."""

    def speak(self):
        return "woof"

    def greet(self):
        return self.speak()
