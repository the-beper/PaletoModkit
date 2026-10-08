// paleto-cwconv: converts GTA V Legacy resources (.ytd .ydr .ydd .yft .ypt)
// to the Enhanced format with CodeWalker's converter, one file at a time.
//
//   paleto-cwconv IN OUT [IN OUT ...]
//
// For each pair it prints one line:
//   converted IN    OUT holds the Enhanced file
//   unchanged IN    IN was already in the Enhanced format; OUT is not written
//   error IN: MSG   nothing was written
// The exit code is 0 unless the arguments are wrong.

using System;
using System.IO;
using CodeWalker.Core.Utils;
using CodeWalker.GameFiles;

static class Program
{
    static int Main(string[] args)
    {
        if (args.Length == 0 || args.Length % 2 != 0)
        {
            Console.Error.WriteLine("usage: paleto-cwconv IN OUT [IN OUT ...]");
            return 2;
        }
        RpfManager.IsGen9 = true;
        for (var i = 0; i < args.Length; i += 2)
        {
            var (input, output) = (args[i], args[i + 1]);
            try
            {
                var data = File.ReadAllBytes(input);
                var ext = Path.GetExtension(input).ToLowerInvariant();
                var result = Gen9Converter.TryConvert(data, ext, _ => { }, input, false, out var converted);
                if (!converted)
                {
                    Console.WriteLine(result == null ? $"error {input}: not a convertible resource" : $"unchanged {input}");
                    continue;
                }
                File.WriteAllBytes(output, result);
                Console.WriteLine($"converted {input}");
            }
            catch (Exception e)
            {
                Console.WriteLine($"error {input}: {e.Message.Replace('\n', ' ')}");
            }
        }
        return 0;
    }
}
