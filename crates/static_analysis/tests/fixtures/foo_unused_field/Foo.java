package com.example;

/**
 * Trivial fixture used by the PMD smoke test. Carries an unused private
 * field (triggers UnusedPrivateField) and a public method without
 * Javadoc so the same fixture also exercises Checkstyle in T3.
 */
public class Foo {

    private int unusedField = 42;

    public int compute(int x) {
        return x * 2;
    }
}
