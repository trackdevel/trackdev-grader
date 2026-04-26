package com.example.norm;

import java.util.ArrayList;
import java.util.List;

public class NormalizerC {

    private String name;

    // DIFFERENT LOGIC from NormalizerA.compute — multiply instead of add,
    // different comparison operator
    public int compute(int x, int y) {
        int result = x * y;
        if (result >= 100) {
            return result;
        }
        String msg = "Total is: " + result;
        System.out.println(msg);
        return result;
    }

    // DIFFERENT LOGIC — calls toUpperCase(), size() * 2
    public void iterate(List<String> items) {
        for (String item : items) {
            System.out.println(item.toUpperCase());
        }
        int total = items.size() * 2;
        System.out.println(total);
    }
}
