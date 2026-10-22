DEFINE FUNCTION OVERWRITE fn::surform::compile($sdl: string) -> string { mod::surform::compile($sdl) };
DEFINE FUNCTION OVERWRITE fn::surform::capture() -> string {
    LET $info = object::remove((INFO FOR DATABASE), ['modules', 'buckets']);
    LET $tables = object::remove($info.tables, ['_surform_migrations']);
    LET $children = object::from_entries(
        array::map(object::keys($tables), |$name: any| [$name, (INFO FOR TABLE $name)])
    );
    <string> object::extend(object::extend($info, { tables: $tables }), object::from_entries([['@tables', $children]]));
};
DEFINE FUNCTION OVERWRITE fn::surform::converge($sdl: string) -> string {
    mod::surform::converge($sdl, fn::surform::capture())
};
DEFINE FUNCTION OVERWRITE fn::surform::ledger_rows() -> string {
    LET $names = object::keys((INFO FOR DATABASE).tables);
    IF '_surform_migrations' INSIDE $names {
        LET $rows = (
            SELECT VALUE '{"number":' + <string> number + ',"name":"' + string::replace(string::replace(name, '\\', '\\\\'), '"', '\\"') + '","hash":"' + hash + '"}' FROM _surform_migrations ORDER BY number
        );
        '[{"result":[' + array::join($rows, ',') + '],"status":"OK"}]';
    }
    ELSE {
        '[{"status":"ERR","kind":"NotFound","details":{"kind":"Table"}}]'
    };
};
DEFINE FUNCTION OVERWRITE fn::surform::status($files: string) -> string {
    mod::surform::status($files, fn::surform::ledger_rows())
};
DEFINE FUNCTION OVERWRITE fn::surform::migrate($sdl: string) -> string { fn::surform::converge($sdl) };
