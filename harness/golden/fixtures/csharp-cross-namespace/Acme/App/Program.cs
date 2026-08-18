using Acme.Util;

namespace Acme.App
{
    public class Program
    {
        public void Run()
        {
            var helper = new TextHelper();
            string flipped = Acme.Util.TextHelper.Reverse("abc");
        }
    }
}
