#[cfg(test)]
mod tests {
    use crate::{Config, MizPath, parser};
    use crate::accom::Accomodator;
    use crate::parser::*;
    use crate::reader::Reader;
    use crate::types::{Article, DirectiveKind, Directives};

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
            xml_internals: false,
            xml_internals_self_test: false,
            json_parse: false,
            overwrite_prel: false,
            cache_prel: false,
            legacy_flex_handling: false,
            attr_sort_bug: false,
            panic_on_fail: false,
            first_verbose_line: None,
            one_item: false,
            skip_to_verbose: false,
        };
        let path = MizPath::new("xboole_0").unwrap();
        let mut reader = Reader::new(&cfg, None, Some(Box::new(Accomodator::default())), path.art);
        let mml_vct = std::fs::read("miz/mizshare/mml.vct").unwrap();
        //path.with_reader(&cfg, None, &mml_vct, &mut |v| v.run_analyzer(&MizPath {}, None));
        let content = path.read_miz().unwrap();
        let write_json = path.write_json(cfg.json_parse);
        let mut parser = MizParser::new(path.art, None, &content, write_json);
        let mut directives = Directives::default();
        parser.parse_env(&mut directives);
        assert_eq!(5, directives.0[DirectiveKind::Vocabularies].len());
        assert_eq!("hidden", directives.0[DirectiveKind::Vocabularies].get(0).unwrap().1.as_str());
        assert_eq!("tarski", directives.0[DirectiveKind::Vocabularies].get(1).unwrap().1.as_str());
        assert_eq!("xboole_0", directives.0[DirectiveKind::Vocabularies].get(2).unwrap().1.as_str());
        assert_eq!("matroid0", directives.0[DirectiveKind::Vocabularies].get(3).unwrap().1.as_str());
        assert_eq!("aofa_000", directives.0[DirectiveKind::Vocabularies].get(4).unwrap().1.as_str());
        //reader.run_analyzer(&path, Some(&mut parser));
        //println!("{:?}", directives);
    }
}