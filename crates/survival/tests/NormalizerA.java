package com.example.norm;

import java.util.ArrayList;
import java.util.List;

public class NormalizerA {

    private String name;

    public int compute(int x, int y) {
        int result = x + y;
        if (result > 100) {
            return 100;
        }
        String msg = "Total is: " + result;
        System.out.println(msg);
        return result;
    }

    public void iterate(List<String> items) {
        for (String item : items) {
            System.out.println(item);
        }
        int total = items.size();
        System.out.println(total);
    }

    public void tryCatch() {
        try {
            int val = Integer.parseInt("42");
            System.out.println(val);
        } catch (NumberFormatException e) {
            System.err.println(e.getMessage());
        }
    }

    public void withLambda(List<String> data) {
        data.forEach(s -> System.out.println(s));
        List<String> filtered = new ArrayList<>();
        data.stream()
            .filter(item -> item.length() > 3)
            .forEach(item -> filtered.add(item));
    }
}
