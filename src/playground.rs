#[cfg(test)]
mod tests {
    use crate::{Config, MizPath, parser};
    use crate::accom::Accomodator;
    use crate::parser::*;
    use crate::reader::Reader;
    use crate::types::{Article, Directives};

    #[test]
    fn playground() {
        let cfg = Config{
            top_item_header: false,
            always_verbose_item: false,
            item_header: false,
            checker_inputs: false,
            checker_header: false,
            checker_conjuncts: false,
            checker_result: false,
            unify_header: false,
            unify_insts: false,
            dump: Default::default(),
            accom_enabled: true,
            parser_enabled: true,
            nameck_enabled: false,
            analyzer_enabled: true,
            analyzer_full: false,
            checker_enabled: false,
            exporter_enabled: false,
            verify_export: false,
            xml_export: false,
            overwrite_prel: false,
            cache_prel: false,
            legacy_flex_handling: false,
            flex_expansion_bug: false,
            attr_sort_bug: false,
            panic_on_fail: false,
            first_verbose_line: None,
            one_item: false,
            skip_to_verbose: false,
        };
        let path = MizPath::new("xboole_0");
        let mut reader = Reader::new(&cfg, None, Some(Box::new(Accomodator::default())), path.art);
        let mml_vct = std::fs::read("miz/mizshare/mml.vct").unwrap();
        //path.with_reader(&cfg, None, &mml_vct, &mut |v| v.run_analyzer(&MizPath {}, None));
        let content = path.read_miz().unwrap();
        let mut parser = MizParser::new(path.art, None, &content);
        parser.parse_env(&mut Default::default());
        reader.run_analyzer(&path, Some(&mut parser));
        //println!("{:?}", directives);
    }
}