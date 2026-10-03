    /// The precondition the three `is_empty()` guards in `build` exist for:
    /// `physical.rs` really does have a primitive with no variants.
    ///
    /// Named for what it checks. It does not call `build` -- no netlist can
    /// reach those guards today, because nothing in the library instantiates a
    /// comparator -- so calling it a refusal test would claim coverage it does
    /// not have. What it pins is that the guards are not dead reasoning.
    #[test]
    fn physical_really_has_a_primitive_with_no_variants() {
        assert!(
            physical::variants(Primitive::Comparator).is_empty(),
            "this test is about the primitive that has none; if that changed, \
             pick another or delete this"
        );
    }
