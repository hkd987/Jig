Feature: Selling
  Scenario: missing, ambiguous and mistyped steps
    Given we have 5 items
    When we sell "two" items to "ann"
#~^ E0804
    Then a refund is issued
#~^ E0802
    And we have 5 widgets

  Scenario: ok
    Given we have 3 widgets
    When we sell 2 items to "bob"
#~^ E0803
