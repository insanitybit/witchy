//! `sealed type`: built, written and taken apart only in the file that declares it.
use crate::diagnostic::Diagnostic;
const LIBRARY:&str="sealed type Meter:\n    data: Buffer<U8>\n    reading: U64\n\nfn meter_new(data: Buffer<U8>) -> Meter:\n    return Meter(move data, 0)\n\nfn meter_bump(meter: &mut Meter):\n    (*meter).reading = (*meter).reading + 1\n    return\n\nfn meter_open(meter: Meter) -> Buffer<U8>:\n    return move meter.data\n\n";
/// The library is one file and `client` is another.
fn checked(client:&str)->Result<(),Diagnostic>{
    let library=crate::compiler::exported(LIBRARY);let source=format!("{library}import 0\n{client}");let syntax=crate::compiler::parse_modules(&source,&[0,library.len()])?;
    let logical=super::check::program(&syntax)?;super::concrete::program(logical)?;Ok(())
}
fn refused(client:&str)->String{checked(client).expect_err(client).message.to_string()}
#[test]
fn another_file_reads_and_passes_a_sealed_value_but_goes_through_its_functions(){
    checked("fn read(meter: &Meter) -> U64:\n    return (*meter).reading\n\nfn round(data: Buffer<U8>) -> Buffer<U8>:\n    var meter = meter_new(move data)\n    meter_bump(&mut meter)\n    return meter_open(move meter)\n\nfn main():\n    return\n").unwrap();
}
#[test]
fn another_file_cannot_construct_write_or_open_a_sealed_value(){
    for client in ["fn forge(data: Buffer<U8>) -> Meter:\n    return Meter(move data, 99)\n\nfn main():\n    return\n",
        "fn tamper(meter: &mut Meter):\n    (*meter).reading = 99\n    return\n\nfn main():\n    return\n",
        "fn tamper(meter: Meter) -> Meter:\n    var mine = move meter\n    mine.reading = 99\n    return move mine\n\nfn main():\n    return\n",
        "fn open(meter: Meter) -> Buffer<U8>:\n    return move meter.data\n\nfn main():\n    return\n",
        "fn open(meter: Meter) -> Buffer<U8>:\n    let Meter(data, reading) = move meter\n    return move data\n\nfn main():\n    return\n"]{
        assert!(refused(client).contains("sealed type"),"{client}");}
}
#[test]
fn the_defining_file_and_unsealed_types_are_unrestricted(){
    let source=format!("{LIBRARY}fn main():\n    return\n");
    super::check::program(&crate::compiler::parse(&source).unwrap()).unwrap();
    let open=crate::compiler::exported(&LIBRARY.replace("sealed type","type"));let source=format!("{open}import 0\nfn forge(data: Buffer<U8>) -> Meter:\n    return Meter(move data, 99)\n\nfn main():\n    return\n");
    super::check::program(&crate::compiler::parse_modules(&source,&[0,open.len()]).unwrap()).unwrap();
}
