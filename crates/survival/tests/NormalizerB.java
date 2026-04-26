package com.example.norm;

import java.util.ArrayList;
import java.util.List;

public class NormalizerB {

    private String name;

    // Same logic as NormalizerA.compute but with different variable names
    public int compute(int a, int b) {
        int sum = a + b;
        if (sum > 100) {
            return 100;
        }
        String message = "Total is: " + sum;
        System.out.println(message);
        return sum;
    }

    // Same logic as NormalizerA.iterate but with different variable names
    public void iterate(List<String> elements) {
        for (String el : elements) {
            System.out.println(el);
        }
        int count = elements.size();
        System.out.println(count);
    }

    // Same logic as NormalizerA.tryCatch but with different variable names
    public void tryCatch() {
        try {
            int number = Integer.parseInt("42");
            System.out.println(number);
        } catch (NumberFormatException ex) {
            System.err.println(ex.getMessage());
        }
    }

    // Same logic as NormalizerA.withLambda but with different variable names.
    // Preserves same unique-name-count structure: two distinct lambda param
    // names (el + entry) matching A's (s + item), so the positional placeholder
    // mapping produces the same result.
    public void withLambda(List<String> input) {
        input.forEach(el -> System.out.println(el));
        List<String> result = new ArrayList<>();
        input.stream()
            .filter(entry -> entry.length() > 3)
            .forEach(entry -> result.add(entry));
    }
}
