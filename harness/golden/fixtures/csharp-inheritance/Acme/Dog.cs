namespace Acme;

/// <summary>A dog.</summary>
public class Dog : Animal {
    public override void Speak() {}
    public void Greet() {
        this.Speak();
    }
}
