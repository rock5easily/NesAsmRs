using System;
using System.IO;
using System.Linq;
using System.Text;
using System.Collections.Generic;
using System.Web.Script.Serialization;
using NesAsmSharp.Assembler;

// Compile the original core without modifying it. Run one process per fixture:
// legacy code changes process-global state and may call Environment.Exit.
public static class Oracle
{
    public static int Main(string[] args)
    {
        var input = Path.GetFullPath(args[0]);
        var output = Path.GetFullPath(args[1]);
        Directory.CreateDirectory(output);
        Environment.CurrentDirectory = Path.GetDirectoryName(input);
        var stdout = new StringWriter();
        var listing = new MemoryStream();
        var encoding = args.Contains("--sjis") ? Encoding.GetEncoding(932) : new UTF8Encoding(false);
        var listingWriter = new StreamWriter(listing, encoding);
        var options = new NesAsmOption {
            InFName = input, BinFName = Path.Combine(output, "rom.nes"),
            OutFName = Path.Combine(output, "rom"), LstFName = Path.Combine(output, "rom.lst"),
            Encoding = encoding, HeaderOpt = !args.Contains("--raw"),
            AutoZPOpt = args.Contains("--autozp"), SrecOpt = args.Contains("--srec"),
            ListLevel = args.Contains("--list3") ? 3 : args.Contains("--list1") ? 1 : args.Contains("--list0") ? 0 : 2,
            MListOpt = args.Contains("--mlist"),
            StdOut = stdout, StdErr = stdout, LstStreamWriter = listingWriter
        };
        Console.SetOut(stdout);
        var assembler = new NesAssembler(options);
        try { assembler.Assemble(); }
        catch (Exception e) { stdout.WriteLine(e.ToString()); }
        listingWriter.Flush();
        var symbols = new SortedDictionary<string, object>();
        // Only defined labels and constants: the table also holds macro and
        // function names and symbols that were referenced but never defined.
        foreach (var s in assembler.Context.GLablHashTbl.Values)
        {
            if (s.Name == null) continue;
            if (s.Type == SymbolFlag.DEFABS)
                symbols[s.Name] = new { value = unchecked((uint)s.Value), bank = s.Bank, size = s.DataSize };
            if (s.Local != null) foreach (var local in s.Local)
                if (local.Type == SymbolFlag.DEFABS)
                    symbols[s.Name + local.Name] = new { value = unchecked((uint)local.Value), bank = local.Bank, size = local.DataSize };
        }
        var regions = assembler.Context.RegionTbl.ToDictionary(p => p.Key, p => (object)new {
            size = p.Value.BeginBank >= 0 && p.Value.EndBank >= 0 ? (long?)p.Value.RegionSize : null
        });
        var binary = assembler.AssembleSuccess ? assembler.ResultBinary : new byte[0];
        var map = assembler.AssembleSuccess ? assembler.ResultMap : new byte[0];
        var romPath = Path.Combine(output, "rom.nes");
        var header = assembler.AssembleSuccess && options.HeaderOpt && File.Exists(romPath)
            ? File.ReadAllBytes(romPath).Take(16).ToArray() : new byte[0];
        var payload = new {
            success = assembler.AssembleSuccess, binary, map, header, symbols, regions,
            listing = encoding.GetString(listing.ToArray()),
            srec = File.Exists(Path.Combine(output, "rom.s28")) ? File.ReadAllText(Path.Combine(output, "rom.s28"), encoding) : null,
            log = stdout.ToString()
        };
        var json = new JavaScriptSerializer { MaxJsonLength = int.MaxValue }.Serialize(payload);
        File.WriteAllText(Path.Combine(output, "result.json"), json, new UTF8Encoding(false));
        return 0;
    }
}
