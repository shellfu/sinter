using System;

namespace Demo
{
    /// <summary>Application entry point.</summary>
    public class Program
    {
        /// <summary>Runs the demo.</summary>
        public void Run()
        {
            string message = Greeter.Greet("world");
            Console.WriteLine(message);
        }
    }
}
