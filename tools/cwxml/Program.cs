// paleto-cwxml: a development check, not shipped. Imports CodeWalker XML
// (.ydr.xml .ydd.xml .yft.xml .ybn.xml) with CodeWalker's own importers, as
// its "Import XML" does, and saves the resource it builds next to the XML:
// a.ydr.xml becomes a.ydr.cw. The DDS files the XML names are read from the
// folder named after the model next to it (a/ for a.ydr.xml), as CodeWalker's
// own export writes them.
//
//   paleto-cwxml [--gen9] FILE.xml [FILE.xml ...]
//
// --gen9 saves Enhanced (gen9) resources, as cwconv does. For each file it prints:
//   ok FILE
//   error FILE: MSG     nothing was written
// The exit code is 0 unless the arguments are wrong.

using System;
using System.Collections.Generic;
using System.IO;
using System.Xml;
using CodeWalker.GameFiles;

static class Program
{
    static int Main(string[] args)
    {
        var gen9 = args.Length > 0 && args[0] == "--gen9";
        var files = gen9 ? args[1..] : args;
        if (files.Length == 0)
        {
            Console.Error.WriteLine("usage: paleto-cwxml [--gen9] FILE.xml [FILE.xml ...]");
            return 2;
        }
        RpfManager.IsGen9 = gen9;
        foreach (var file in files)
        {
            try
            {
                if (!file.EndsWith(".xml", StringComparison.OrdinalIgnoreCase))
                {
                    throw new Exception("not an .xml file");
                }
                var output = file[..^4] + ".cw";
                var ext = Path.GetExtension(file[..^4]).ToLowerInvariant();
                var folder = Path.Combine(Path.GetDirectoryName(Path.GetFullPath(file)), Path.GetFileNameWithoutExtension(file[..^4]));
                var doc = new XmlDocument();
                doc.LoadXml(File.ReadAllText(file));
                byte[] data;
                var missing = new List<string>();
                switch (ext)
                {
                    case ".ydr":
                        var ydr = XmlYdr.GetYdr(doc, folder);
                        if (ydr.Drawable == null) throw new Exception("no drawable imported");
                        Missing(ydr.Drawable, missing);
                        data = ydr.Save();
                        break;
                    case ".ydd":
                        var ydd = XmlYdd.GetYdd(doc, folder);
                        if (ydd.DrawableDict == null) throw new Exception("no drawable dictionary imported");
                        foreach (var d in ydd.DrawableDict.Drawables?.data_items ?? []) Missing(d, missing);
                        data = ydd.Save();
                        break;
                    case ".yft":
                        var yft = XmlYft.GetYft(doc, folder);
                        if (yft.Fragment == null) throw new Exception("no fragment imported");
                        Missing(yft.Fragment.Drawable, missing);
                        data = yft.Save();
                        break;
                    case ".ybn":
                        var ybn = XmlYbn.GetYbn(doc);
                        if (ybn.Bounds == null) throw new Exception("no bounds imported");
                        data = ybn.Save();
                        break;
                    default:
                        throw new Exception($"unsupported kind {ext}");
                }
                // CodeWalker leaves a texture empty when its DDS is missing or unreadable.
                if (missing.Count > 0)
                {
                    throw new Exception("textures without data (DDS missing or unreadable): " + string.Join(", ", missing));
                }
                if (data == null || data.Length == 0)
                {
                    throw new Exception("nothing saved");
                }
                File.WriteAllBytes(output, data);
                Console.WriteLine($"ok {file}");
            }
            catch (Exception e)
            {
                var where = e.StackTrace?.Split('\n')[0].Trim() ?? "";
                Console.WriteLine($"error {file}: {e.GetType().Name}: {e.Message.Replace('\n', ' ')} {where}");
            }
        }
        return 0;
    }

    /// Embedded textures of a drawable that have no pixel data.
    static void Missing(DrawableBase drawable, List<string> missing)
    {
        foreach (var tex in drawable?.ShaderGroup?.TextureDictionary?.Textures?.data_items ?? [])
        {
            if (tex?.Data?.FullData == null)
            {
                missing.Add(tex?.Name ?? "?");
            }
        }
    }
}
