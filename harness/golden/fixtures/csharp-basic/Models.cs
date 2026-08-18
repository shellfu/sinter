namespace Demo
{
    /// <summary>Anything that can speak.</summary>
    public interface ISpeaker
    {
        void Speak();
    }

    /// <summary>A 2D point.</summary>
    public struct Point
    {
        public int X { get; set; }
    }

    /// <summary>An immutable note.</summary>
    public record Note
    {
        public string Body { get; set; }
    }

    public enum Tone
    {
        Casual,
        Formal
    }
}
