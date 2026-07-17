using System.Linq;

namespace App
{
    // The first half of a partial class. `RenderActive` uses a LINQ
    // query_expression whose `select helper(x)` call lives inside the
    // query body, and `helper` is defined in the partial companion
    // (Greeter.Part.cs). Indexing both — LINQ capture inside the query and
    // the partial-class cross-file merge — is the issues.md #125 fixture.
    public partial class Greeter
    {
        private readonly Item[] _items;

        public Greeter(Item[] items)
        {
            _items = items;
        }

        public string RenderActive()
        {
            var active = from x in _items where x.Active select helper(x);
            return JoinWith(active);
        }

        // Same-file Calls edge (RenderActive -> JoinWith) — independent of the
        // partial-class companion, so the #238 golden can assert a call edge
        // before the #125 partial-merge work lands.
        private string JoinWith(System.Collections.Generic.IEnumerable<string> items)
        {
            return string.Join(",", items);
        }
    }
}
