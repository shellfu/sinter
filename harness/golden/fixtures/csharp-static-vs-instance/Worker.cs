namespace Demo
{
    public class Worker
    {
        public void Run()
        {
            Worker.Prepare();
            this.Step();
            Helper typed = new Helper();
            typed.Assist();
            var untyped = new Helper();
            untyped.Assist();
        }

        public static void Prepare()
        {
        }

        public void Step()
        {
        }
    }

    public class Helper
    {
        public void Assist()
        {
        }
    }
}
