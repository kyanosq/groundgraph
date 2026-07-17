namespace App
{
    // The second half of the partial Greeter. Without a partial-class merge
    // this is a separate `csharp::App/Greeter.Part.cs::Greeter` node and the
    // `helper` call from RenderActive in Greeter.cs cannot resolve across the
    // file boundary (issues.md #125).
    public partial class Greeter
    {
        public string helper(Item x)
        {
            return x.Name;
        }
    }
}
